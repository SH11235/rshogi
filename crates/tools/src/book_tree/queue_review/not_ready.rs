use super::*;

pub(super) struct Continuations {
    pub outputs: Vec<PathBuf>,
    pub payloads: Vec<Vec<u8>>,
}

pub(super) fn build(
    args: &QueueReviewArgs,
    boxes: &[BoxData],
    metrics: &[BoxMetrics],
    statuses: &[Vec<per_tree::Status>],
    inputs: &mut Vec<PathBuf>,
) -> Result<Continuations> {
    let mut sent = BTreeSet::new();
    for path in &args.not_ready_sent {
        inputs.push(path.clone());
        for (n, text) in BufReader::new(File::open(path)?).lines().enumerate() {
            let text = text?;
            if text.trim().is_empty() {
                continue;
            }
            let raw: Value = serde_json::from_str(&text)
                .with_context(|| format!("{}:{}", path.display(), n + 1))?;
            sent.insert(raw["id"].as_str().context("送信済みの行に id がありません")?.to_owned());
        }
    }
    let live = boxes
        .iter()
        .map(|b| b.dir.join("END").try_exists().map(|v| !v))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut copies: BTreeMap<(&str, &str), Vec<(usize, &per_tree::Status)>> = BTreeMap::new();
    for (i, b) in boxes.iter().enumerate() {
        for status in &statuses[i] {
            let q = &b.queue[&status.id];
            copies.entry((&q.primary_root, &status.own)).or_default().push((i, status));
        }
    }
    let mut table = b"verdict\twhy\tbox\tid\town\tpriority\tread\trequired\trounds\tband_remaining\tband_hours\tcutoff_hours\tcopies\n".to_vec();
    let mut rows = vec![Vec::new(); boxes.len()];
    for (i, b) in boxes.iter().enumerate() {
        let mut decisions = Vec::new();
        for status in &statuses[i] {
            if status.state != "NOT_READY" {
                continue;
            }
            let q = &b.queue[&status.id];
            let opts = q.raw.get("opts").and_then(Value::as_object);
            let rounds = opts.and_then(|v| v.get("rounds")).and_then(Value::as_u64);
            let mut blocked = BTreeSet::new();
            if let Some(others) = copies.get(&(q.primary_root.as_str(), status.own.as_str())) {
                for &(j, other) in others {
                    if j == i && other.id == status.id {
                        continue;
                    }
                    let other_q = &boxes[j].queue[&other.id];
                    if other.state == "DONE"
                        || (live[j]
                            && !other_q.line.hold
                            && matches!(other.state.as_str(), "RUNNING" | "QUEUED"))
                    {
                        blocked.insert(format!("{}:{}:{}", boxes[j].name, other.id, other.state));
                    }
                }
            }
            let amount = per_tree::ahead(b, &statuses[i], args.tree_size, q.line.priority)?
                .checked_add(args.tree_size)
                .context("帯の量が大きすぎます")?;
            let hours = if amount == 0 {
                0.0
            } else {
                amount as f64 / metrics[i].speed
            };
            let (verdict, why) = if !live[i] {
                ("終わった台", "台の dir に END がある")
            } else if q.line.hold {
                ("hold", "保留の木")
            } else if opts.is_none() {
                ("opts なし", "受付の最後の行に opts が無い")
            } else if rounds.is_some_and(|r| r >= 6) {
                ("続きを積んだ後も NOT_READY", "受付の最後の行が既に rounds 6 以上")
            } else if sent.contains(&status.id) {
                ("直前に送った", "指定された送信済み JSONL に id がある")
            } else if !blocked.is_empty() {
                ("手当て済み", "同じ主根・同じ own の DONE または稼働中の写しがある")
            } else if hours > metrics[i].hours {
                ("帯で回らない", "この木の続きの分を足すと締めを越える")
            } else {
                ("送る", "rounds を 6 にする")
            };
            decisions.push((status, rounds, blocked, amount, hours, verdict, why));
        }
        let eligible: BTreeSet<_> = decisions
            .iter()
            .filter(|d| d.5 == "送る")
            .map(|d| (d.0.id.as_str(), d.0.own.as_str()))
            .collect();
        for (status, rounds, blocked, amount, hours, verdict, why) in &decisions {
            let (mut verdict, mut why) = (*verdict, *why);
            let q = &b.queue[&status.id];
            if verdict == "送る" {
                if q.line
                    .sides()
                    .iter()
                    .all(|own| eligible.contains(&(status.id.as_str(), own.as_str())))
                {
                    let mut raw = q.raw.clone();
                    raw["opts"]["rounds"] = Value::from(6);
                    if !rows[i].iter().any(|r: &Value| r["id"] == raw["id"]) {
                        rows[i].push(raw);
                    }
                } else {
                    verdict = "片側だけ候補";
                    why = "bw の両側が送る判定でないため受付行を出さない";
                }
            }
            writeln!(
                table,
                "{verdict}\t{why}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{amount}\t{hours}\t{}\t{}",
                b.name,
                status.id,
                status.own,
                q.line.priority,
                status.read,
                status.required,
                rounds.map(|v| v.to_string()).unwrap_or_default(),
                metrics[i].hours,
                blocked.iter().cloned().collect::<Vec<_>>().join(" | ")
            )?;
        }
    }
    let mut outputs = vec![args.not_ready_table.clone().context("not-ready-table が必要です")?];
    let mut payloads = vec![table];
    let dir = args.not_ready_out.as_ref().context("not-ready-out が必要です")?;
    for (b, rows) in boxes.iter().zip(rows) {
        outputs.push(dir.join(format!("{}_not_ready_{}.jsonl", args.prefix, b.name)));
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, &row)?;
            writeln!(bytes)?;
        }
        payloads.push(bytes);
    }
    Ok(Continuations { outputs, payloads })
}
