//! 読みの比較から元データの追加候補と報告を作る。
use super::{
    add::{read_table, sort_rows},
    export::{
        Schema, files, nonnegative_integer, numbers,
        reads::{self, Groups, Reading},
    },
    master_table::{self, Row},
};
use crate::book_db::position_from_sfen;
use anyhow::{Context, Result, ensure};
use clap::Args;
use num_bigint::BigInt;
use num_rational::BigRational;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::PathBuf,
};

#[derive(Args, Debug)]
pub(super) struct AddReadsArgs {
    /// 元データの表
    #[arg(long)]
    master: PathBuf,
    /// 追加候補の表の出力先
    #[arg(long)]
    out: PathBuf,
    /// 追加候補の版のタグ
    #[arg(long)]
    since: String,
    /// 取り込む読みのノード数の下限
    #[arg(long, default_value="900000000", value_parser=nonnegative_integer)]
    min_nodes: BigInt,
    /// draw 未指定の読みを除く先頭評価値の絶対値の上限
    #[arg(long, default_value="3", value_parser=nonnegative_integer)]
    draw_pin: BigInt,
    /// JSONL ファイル、または再帰的に探すディレクトリ
    #[arg(required=true, num_args=1..)]
    logs: Vec<PathBuf>,
}
const REPORT_FIELDS: [&str; 15] = [
    "key",
    "reads",
    "shallow_dropped",
    "move",
    "role",
    "mean_gap",
    "firsts",
    "status",
    "reason",
    "existing_moves",
    "compared_reads",
    "multipv_lengths",
    "compare_n",
    "gaps",
    "reread",
];
struct Selection {
    rows: Vec<Row>,
    report: Vec<[String; 15]>,
}
fn score<'a>(reading: &'a Reading, mv: &str) -> Option<&'a BigRational> {
    reading.scores.iter().find(|(m, _)| m == mv).map(|(_, v)| v)
}
fn select(master: &[Row], groups: &Groups, since: &str) -> Result<Selection> {
    let mut existing = BTreeMap::<&str, BTreeSet<&str>>::new();
    for row in master {
        existing.entry(row.get("key")).or_default().insert(row.get("move"));
    }
    let mut rows = Vec::new();
    let mut report = Vec::new();
    let twenty = BigRational::from_integer(20.into());
    let forty = BigRational::from_integer(40.into());
    for (key, all) in groups {
        let Some(ply) = all.iter().map(|r| &r.ply).min() else {
            continue;
        };
        let readings = reads::deep(all);
        if readings.is_empty() {
            continue;
        }
        let lengths: Vec<_> = readings.iter().map(|r| r.scores.len()).collect();
        let n = *lengths.iter().min().unwrap();
        let moves: BTreeSet<_> =
            readings.iter().flat_map(|r| r.scores.iter().map(|(m, _)| m.as_str())).collect();
        let mut gaps = BTreeMap::new();
        let mut wins = BTreeMap::new();
        let mut differences = BTreeMap::new();
        let mut appearances = BTreeMap::new();
        for &mv in &moves {
            let diffs: Vec<_> =
                readings.iter().map(|r| score(r, mv).map(|v| &r.scores[0].1 - v)).collect();
            let gap: BigRational = diffs.iter().flatten().cloned().sum::<BigRational>()
                / BigInt::from(diffs.iter().flatten().count());
            gaps.insert(mv, gap);
            wins.insert(mv, readings.iter().filter(|r| r.scores[0].0 == mv).count());
            differences.insert(mv, diffs);
            appearances.insert(
                mv,
                readings.iter().filter(|r| r.scores[..n].iter().any(|(m, _)| m == mv)).count(),
            );
        }
        let pos = position_from_sfen(&format!("{key} 1"))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let legal: BTreeSet<_> = legal.iter().map(|m| m.to_usi()).collect();
        let mut chosen: Vec<_> = moves
            .iter()
            .copied()
            .filter(|m| {
                legal.contains(*m)
                    && appearances[m] == readings.len()
                    && gaps[m] < twenty
                    && differences[m].iter().all(|d| d.as_ref().is_some_and(|d| *d < forty))
            })
            .collect();
        let mut records: BTreeSet<_> = moves
            .iter()
            .copied()
            .filter(|m| gaps[m] < forty && !chosen.contains(m))
            .collect();
        let mut reason = "consensus";
        if readings.len() == 1 {
            chosen = vec![readings[0].scores[0].0.as_str()];
            records.clear();
            reason = "single";
        } else if chosen.is_empty() {
            chosen = vec![readings.last().unwrap().scores[0].0.as_str()];
            reason = "fallback";
        }
        chosen.sort_by(|a, b| {
            gaps[a].cmp(&gaps[b]).then_with(|| wins[b].cmp(&wins[a])).then_with(|| a.cmp(b))
        });
        let accepted: Vec<_> = chosen.iter().copied().filter(|m| legal.contains(*m)).collect();
        let nodes = readings.iter().map(|r| &r.nodes).max().unwrap();
        let old = existing.get(key.as_str()).cloned().unwrap_or_default();
        for mv in moves {
            let mut role = "";
            let mut status = "not_selected";
            if !legal.contains(mv) {
                status = "illegal";
            } else if chosen.contains(&mv) || records.contains(mv) {
                if old.contains(mv) {
                    status = "existing";
                } else {
                    role = if !old.is_empty() || !chosen.contains(&mv) {
                        "record"
                    } else if accepted.first() == Some(&mv) {
                        "main"
                    } else {
                        "alt"
                    };
                    let mut why = Vec::new();
                    if role == "record" {
                        if !old.is_empty() {
                            why.push("土台の手を優先".to_owned());
                        }
                        let present = differences[mv].iter().flatten().count();
                        if present < readings.len() {
                            why.push(format!("{} 回のうち {present} 回だけ出た", readings.len()));
                        }
                        if readings.iter().any(|r| r.scores[n..].iter().any(|(m, _)| m == mv)) {
                            why.push("比べる範囲の外".into());
                        }
                        if gaps[mv] >= twenty {
                            why.push(format!(
                                "差の平均 {} (20 以上)",
                                numbers::display(&gaps[mv])?
                            ));
                        }
                        if differences[mv].iter().flatten().any(|d| *d >= forty) {
                            why.push("差が 40 以上の回あり".into());
                        }
                    }
                    status = "add";
                    let evals = readings
                        .iter()
                        .filter_map(|r| score(r, mv))
                        .map(|value| {
                            ensure!(
                                value.is_integer(),
                                "cp は整数にしてください: {key} | {mv}: {value}"
                            );
                            Ok(value.to_integer().to_string())
                        })
                        .collect::<Result<Vec<_>>>()?
                        .join(",");
                    master_table::mean_eval(&evals)
                        .with_context(|| format!("evals が不正です: {key} | {mv}: {evals}"))?;
                    let mut note = format!(
                        "約 {} 億ノードの読み {} 回。1 位 {} 回、1 位からの差の平均 {}",
                        numbers::node_note(nodes)?,
                        readings.len(),
                        wins[mv],
                        numbers::display(&gaps[mv])?
                    );
                    if !why.is_empty() {
                        note.push_str(&format!("。record: {}", why.join("、")));
                    }
                    rows.push(Row::from_fields(&[
                        ("key", key.clone()),
                        ("ply", ply.to_string()),
                        ("move", mv.into()),
                        ("role", role.into()),
                        ("source", "read".into()),
                        ("tier", "wide".into()),
                        ("since", since.into()),
                        ("evals", evals),
                        ("nodes", numbers::rounded_nodes(nodes)?),
                        ("note", note),
                    ]));
                }
            }
            let reread = readings.len() == 1
                && readings[0].scores.len() > 1
                && &readings[0].scores[0].1 - &readings[0].scores[1].1
                    < BigRational::from_integer(10.into());
            report.push([
                key.clone(),
                readings.len().to_string(),
                (all.len() - readings.len()).to_string(),
                mv.into(),
                role.into(),
                numbers::display(&gaps[mv])?,
                wins[mv].to_string(),
                status.into(),
                reason.into(),
                old.iter().copied().collect::<Vec<_>>().join(","),
                appearances[mv].to_string(),
                lengths.iter().map(usize::to_string).collect::<Vec<_>>().join(","),
                n.to_string(),
                differences[mv]
                    .iter()
                    .map(|d| d.as_ref().map_or_else(|| Ok("NA".into()), numbers::display))
                    .collect::<Result<Vec<_>>>()?
                    .join(","),
                if reread { "True" } else { "False" }.into(),
            ]);
        }
    }
    sort_rows(&mut rows);
    Ok(Selection { rows, report })
}
fn report_bytes(report: &[[String; 15]]) -> Vec<u8> {
    let mut text = REPORT_FIELDS.join("\t");
    text.push('\n');
    for row in report {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                text.push('\t');
            }
            if cell.contains(['\t', '\r', '\n', '"']) {
                text.push('"');
                text.push_str(&cell.replace('"', "\"\""));
                text.push('"');
            } else {
                text.push_str(cell);
            }
        }
        text.push('\n');
    }
    text.into_bytes()
}
pub(super) fn run(args: AddReadsArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}
fn run_to(args: AddReadsArgs, out: &mut impl Write) -> Result<()> {
    ensure!(
        !args.since.trim().is_empty() && !args.since.contains(['\t', '\r', '\n']),
        "since must be nonempty and contain no tabs or newlines"
    );
    let logs = reads::expand(&args.logs)?;
    let mut report_path = args.out.as_os_str().to_owned();
    report_path.push(".report.tsv");
    let outputs = [args.out, PathBuf::from(report_path)];
    let mut inputs = vec![args.master.clone()];
    inputs.extend(logs.iter().cloned());
    files::guard(&outputs, &inputs, false)?;
    let (mut master, mut errors) = read_table(&args.master)?;
    errors.extend(Schema::default().validate(master_table::format_master(&master))?);
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    let (groups, counts) = reads::load(&logs, args.min_nodes, args.draw_pin, None)?;
    let Selection { rows, report } = select(&master, &groups, &args.since)?;
    master.extend(rows.iter().cloned());
    let errors = Schema::default().validate(master_table::format_master(&master))?;
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    files::write(
        &outputs,
        &inputs,
        false,
        &[
            master_table::format_master(&rows).into_bytes(),
            report_bytes(&report),
        ],
    )?;
    writeln!(
        out,
        "reads: lines={}, used={}, malformed={}, below_min={}, below_min_positions={}, draw_pinned={}, draw_unset_used={}, duplicates={}, illegal_top={}, unsorted={}",
        counts.lines,
        counts.used,
        counts.malformed,
        counts.below_min,
        counts.below_min_positions,
        counts.draw_pinned,
        counts.draw_unset_used,
        counts.duplicates,
        counts.illegal_top,
        counts.unsorted
    )?;
    let dropped: Vec<_> = groups.values().map(|rs| rs.len() - reads::deep(rs).len()).collect();
    writeln!(
        out,
        "shallow_dropped={}, shallow_positions={}",
        dropped.iter().sum::<usize>(),
        dropped.iter().filter(|n| **n > 0).count()
    )?;
    writeln!(
        out,
        "target positions: {}; added moves: {}; new positions (main): {}; new positions with alt: {}; existing-position records: {}",
        groups.len(),
        rows.len(),
        rows.iter().filter(|r| r.get("role") == "main").count(),
        rows.iter()
            .filter(|r| r.get("role") == "alt")
            .map(|r| r.get("key"))
            .collect::<BTreeSet<_>>()
            .len(),
        rows.iter().filter(|r| r.get("role") == "record").count()
    )?;
    writeln!(
        out,
        "illegal position/moves: {}",
        report.iter().filter(|r| r[7] == "illegal").count()
    )?;
    for side in ["b", "w"] {
        for (low, high, label) in [
            (1, Some(20), "1-20"),
            (21, Some(40), "21-40"),
            (41, Some(60), "41-60"),
            (61, None, "61+"),
        ] {
            let selected: Vec<_> = rows
                .iter()
                .filter(|r| {
                    let ply: BigInt = r.get("ply").parse().expect("検査済みの手数");
                    r.get("key").split_whitespace().nth(1) == Some(side)
                        && ply >= BigInt::from(low)
                        && high.is_none_or(|hi| ply <= BigInt::from(hi))
                })
                .collect();
            writeln!(
                out,
                "added {side} ply {label}: positions={}, moves={}",
                selected.iter().map(|r| r.get("key")).collect::<BTreeSet<_>>().len(),
                selected.len()
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
