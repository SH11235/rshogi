use super::*;

pub(super) struct Plan {
    pub outputs: Vec<PathBuf>,
    pub payloads: Vec<Vec<u8>>,
    pub remaining: Vec<f64>,
    /// 台ごとの priority ごとの見込み量。serve は priority の小さい順に読むので、
    /// 木が締めまでに読めるかは自分の priority 以下の量で決まる。
    load: Vec<BTreeMap<u32, f64>>,
    rows: Vec<Vec<Value>>,
    table: Vec<u8>,
}
impl Plan {
    fn put(&mut self, i: usize, priority: u32, amount: f64) {
        self.remaining[i] += amount;
        *self.load[i].entry(priority).or_default() += amount;
    }
    fn take(&mut self, i: usize, priority: u32, amount: f64) {
        self.remaining[i] = (self.remaining[i] - amount).max(0.0);
        if let Some(value) = self.load[i].get_mut(&priority) {
            *value -= amount;
            if *value <= 0.0 {
                self.load[i].remove(&priority);
            }
        }
    }
    fn ahead(&self, i: usize, priority: u32) -> f64 {
        self.load[i].range(..=priority).map(|(_, v)| v).sum()
    }
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

fn hours(amount: f64, speed: f64) -> f64 {
    if amount == 0.0 {
        0.0
    } else if speed == 0.0 {
        f64::INFINITY
    } else {
        amount / speed
    }
}

pub(super) fn load(paths: &[PathBuf], inputs: &mut Vec<PathBuf>) -> Result<Vec<Queued>> {
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

pub(super) fn build<'a>(
    args: &QueueReviewArgs,
    boxes: &'a [BoxData],
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
                        && name != format!("{}_plan.tsv", args.prefix)
                        && name != format!("{}_bands.tsv", args.prefix),
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
    ensure!(
        args.bands.windows(2).all(|w| w[0] < w[1]),
        "bands は小さい順に重複なく並べてください"
    );
    if let Some(hours) = args.balance_hours {
        nonnegative(hours)?;
    }
    let mut updated_ids = BTreeSet::new();
    let mut plan = Plan {
        outputs: Vec::new(),
        payloads: Vec::new(),
        remaining: metrics.iter().map(|m| m.remaining).collect(),
        load: boxes
            .iter()
            .map(|b| {
                let mut load = BTreeMap::<u32, f64>::new();
                for t in b.progress.trees.iter().filter(|t| !t.finished && !t.hold) {
                    *load.entry(t.priority).or_default() += t.remaining(args.tree_size) as f64;
                }
                load
            })
            .collect(),
        rows: vec![Vec::new(); boxes.len()],
        table: b"action\tid\town\tpriority\tfrom\tto\treason\n".to_vec(),
    };
    let initial_load = plan.load.clone();
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
                plan.put(i, original.line.priority, args.update_size as f64);
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
    // 行き先の候補のうち、評価値の最小の台 (同点は名前順)。
    let best = |candidates: Vec<usize>, value: &dyn Fn(usize) -> f64| {
        candidates.into_iter().min_by(|&a, &b| {
            value(a).total_cmp(&value(b)).then_with(|| boxes[a].name.cmp(&boxes[b].name))
        })
    };
    let candidates = |i: usize| {
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
        candidates
    };
    // bw は受付行を分割できないため、両側の見込み量を一緒に移す。
    let amount_of = |i: usize, q: &Queued| {
        boxes[i]
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
            }
    };
    let movable = |plan: &Plan, i: usize, j: usize, q: &Queued| {
        j != i
            && !boxes[j].queue.contains_key(&q.line.id)
            && !plan.rows[j].iter().any(|r| r["id"] == q.line.id)
            && (labels[i] == labels[j] || !shared(q))
    };
    let shift = |plan: &mut Plan, i: usize, destination: Option<usize>, q: &Queued, amount: f64| {
        let source = plan.rows[i].iter().find(|r| r["id"] == q.line.id).unwrap_or(&q.raw).clone();
        plan.emit(i, updated(&source, None, true));
        plan.take(i, q.line.priority, amount);
        if let Some(j) = destination {
            plan.emit(j, updated(&source, None, false));
            plan.put(j, q.line.priority, amount);
        }
    };
    let mut removed = BTreeSet::new();
    let mut kept = BTreeSet::new();
    let overflow = |plan: &mut Plan,
                    removed: &mut BTreeSet<(usize, &'a str)>,
                    kept: &mut BTreeSet<(usize, &'a str)>|
     -> Result<()> {
        for &i in &order {
            let b = &boxes[i];
            for (q, low) in candidates(i) {
                if ratio(plan.remaining[i], metrics[i].capacity) <= args.max_ratio {
                    break;
                }
                if removed.contains(&(i, q.line.id.as_str())) {
                    continue;
                }
                // 自分の priority 以下の量が読める量に収まる木は、後ろの木を外せば読めるので動かさない。
                if !low
                    && ratio(plan.ahead(i, q.line.priority), metrics[i].capacity) <= args.max_ratio
                {
                    continue;
                }
                let amount = amount_of(i, q);
                let p = q.line.priority;
                let destination = if low {
                    None
                } else {
                    let available = order
                        .iter()
                        .copied()
                        .filter(|&j| {
                            movable(plan, i, j, q)
                                && ratio(plan.ahead(j, p) + amount, metrics[j].capacity)
                                    <= args.max_ratio
                        })
                        .collect();
                    best(available, &|j| ratio(plan.ahead(j, p) + amount, metrics[j].capacity))
                };
                if low || destination.is_some() || p >= 100 {
                    shift(plan, i, destination, q, amount);
                    removed.insert((i, q.line.id.as_str()));
                    if let Some(j) = destination {
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
                } else if kept.insert((i, q.line.id.as_str())) {
                    plan.record("keep-over", q, &b.name, &b.name, "priority-below-100")?;
                }
            }
        }
        Ok(())
    };
    overflow(&mut plan, &mut removed, &mut kept)?;
    if let (Some(limit), Some(limit_hours)) = (args.balance_priority, args.balance_hours) {
        let finish = |plan: &Plan, j: usize, extra: f64| {
            hours(plan.ahead(j, limit) + extra, metrics[j].speed)
        };
        for &i in &order {
            let mut band: Vec<_> =
                candidates(i).into_iter().filter(|(q, _)| q.line.priority <= limit).collect();
            band.sort_by(|(a, _), (b, _)| {
                b.line.priority.cmp(&a.line.priority).then_with(|| b.line.id.cmp(&a.line.id))
            });
            for (q, _) in band {
                if finish(&plan, i, 0.0) <= limit_hours {
                    break;
                }
                if removed.contains(&(i, q.line.id.as_str())) {
                    continue;
                }
                let amount = amount_of(i, q);
                let p = q.line.priority;
                let available = order
                    .iter()
                    .copied()
                    .filter(|&j| {
                        movable(&plan, i, j, q)
                            && finish(&plan, j, amount) <= limit_hours
                            && ratio(plan.ahead(j, p) + amount, metrics[j].capacity)
                                <= args.max_ratio
                    })
                    .collect();
                if let Some(j) = best(available, &|j| finish(&plan, j, amount)) {
                    shift(&mut plan, i, Some(j), q, amount);
                    removed.insert((i, q.line.id.as_str()));
                    plan.record("balance", q, &boxes[i].name, &boxes[j].name, "band-hours")?;
                }
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
        let p = q.line.priority;
        let fill = |plan: &Plan, i: usize, extra: f64| {
            ratio(plan.ahead(i, p) + extra, metrics[i].capacity)
        };
        let available = allowed
            .iter()
            .copied()
            .filter(|&i| fill(&plan, i, amount) <= args.max_ratio)
            .collect();
        // hold の入力行は読ませない指定なので、読める量を使わずに hold のまま置く。
        let destination = if q.line.hold {
            None
        } else {
            best(available, &|i| fill(&plan, i, amount))
        };
        let held = destination.is_none();
        let i = destination
            .or_else(|| best(allowed, &|i| ratio(plan.remaining[i], metrics[i].capacity)))
            .context("台がありません")?;
        plan.emit(i, updated(&q.raw, None, held));
        if !held {
            plan.put(i, p, amount);
            for root in &q.roots {
                root_labels.entry(root.clone()).or_default().insert(labels[i].clone());
            }
        }
        plan.record(
            if held { "add-hold" } else { "add" },
            q,
            "",
            &boxes[i].name,
            if q.line.hold {
                "input-hold"
            } else if held {
                "no-capacity"
            } else {
                "capacity"
            },
        )?;
    }
    // 新しい木の割り振りで後ろに押し出された木を、もう一度移すか保留にする。
    overflow(&mut plan, &mut removed, &mut kept)?;
    let mut bands = b"box\tpriority_lo\tpriority_hi\tbefore_remaining\tbefore_hours\tafter_remaining\tafter_hours\tcutoff_hours\n".to_vec();
    for &i in &order {
        let mut low = 0u32;
        for high in args.bands.iter().copied().map(Some).chain([None]) {
            let in_band = |load: &BTreeMap<u32, f64>| -> (f64, f64) {
                let upto = high.unwrap_or(u32::MAX);
                // 空の f64 の和は -0 になるので 0 から足す。
                (
                    load.range(low..=upto).fold(0.0, |sum, (_, v)| sum + v),
                    load.range(..=upto).fold(0.0, |sum, (_, v)| sum + v),
                )
            };
            let (before, before_total) = in_band(&initial_load[i]);
            let (after, after_total) = in_band(&plan.load[i]);
            writeln!(
                bands,
                "{}\t{low}\t{}\t{before}\t{}\t{after}\t{}\t{}",
                boxes[i].name,
                high.map(|h| h.to_string()).unwrap_or_default(),
                hours(before_total, metrics[i].speed),
                hours(after_total, metrics[i].speed),
                metrics[i].hours
            )?;
            match high.and_then(|h| h.checked_add(1)) {
                Some(next) => low = next,
                None => break,
            }
        }
    }
    plan.outputs.push(dir.join(format!("{}_plan.tsv", args.prefix)));
    plan.payloads.push(std::mem::take(&mut plan.table));
    plan.outputs.push(dir.join(format!("{}_bands.tsv", args.prefix)));
    plan.payloads.push(bands);
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
