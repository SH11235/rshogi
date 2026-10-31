//! 元データから定跡・プリセット・注釈を書き出す。
mod files;
mod graph;
mod json;
mod numbers;
mod reads;
#[cfg(test)]
mod tests;

use super::{
    book_diff, check,
    master_table::{self, ROLES, Row},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use json::Json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Args, Debug)]
pub(super) struct ExportArgs {
    #[arg(long)]
    master: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long,default_value="wide",value_parser=["wide","curated"])]
    tier: String,
    #[arg(long)]
    schema: Option<PathBuf>,
    #[arg(long,num_args=1..)]
    reads: Vec<PathBuf>,
    #[arg(long, default_value = "900000000", value_parser = nonnegative_integer)]
    min_nodes: num_bigint::BigInt,
    #[arg(long, default_value = "3", value_parser = nonnegative_integer)]
    draw_pin: num_bigint::BigInt,
    #[arg(long,default_value_t=-150.0,allow_hyphen_values=true)]
    bad_eval: f64,
    #[arg(long)]
    strict_bad_eval: bool,
    #[arg(long)]
    base: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}
fn nonnegative_integer(text: &str) -> std::result::Result<num_bigint::BigInt, String> {
    let value: num_bigint::BigInt = text.parse().map_err(|e| format!("{e}"))?;
    if value < num_bigint::BigInt::from(0) {
        return Err("expected nonnegative integer".into());
    }
    Ok(value)
}

#[derive(Default)]
struct Schema {
    sources: Vec<(String, String)>,
}
impl Schema {
    fn load(dir: Option<&Path>) -> Result<Self> {
        let Some(dir) = dir else {
            return Ok(Self::default());
        };
        ensure!(dir.is_dir(), "schema is not a directory: {}", dir.display());
        let text = std::fs::read_to_string(dir.join("sources.tsv"))?;
        let mut lines = text.lines();
        ensure!(lines.next() == Some("source\tdescription"), "invalid sources.tsv header");
        let mut sources = Vec::new();
        let mut seen = BTreeSet::new();
        for (i, line) in lines.enumerate() {
            let fields: Vec<_> = line.split('\t').collect();
            ensure!(
                fields.len() == 2
                    && !fields[0].trim().is_empty()
                    && !fields[1].trim().is_empty()
                    && !line.contains('\r'),
                "sources.tsv:{}: invalid row",
                i + 2
            );
            ensure!(seen.insert(fields[0]), "sources.tsv:{}: duplicate source", i + 2);
            sources.push((fields[0].into(), fields[1].into()));
        }
        Ok(Self { sources })
    }
}
pub(super) fn run(args: ExportArgs) -> Result<()> {
    run_to(args, &mut std::io::BufWriter::new(std::io::stdout().lock()))
}
fn run_to(args: ExportArgs, out: &mut impl Write) -> Result<()> {
    ensure!(args.bad_eval.is_finite(), "bad-eval must be finite");
    let schema = Schema::load(args.schema.as_deref())?;
    let logs = if args.reads.is_empty() {
        vec![]
    } else {
        reads::expand(&args.reads)?
    };
    let mut inputs = vec![args.master.clone()];
    inputs.extend(logs.iter().cloned());
    if let Some(base) = &args.base {
        inputs.push(base.clone());
    }
    if let Some(dir) = &args.schema {
        inputs.push(dir.join("sources.tsv"));
    }
    let outputs = files::outputs(&args.out);
    files::guard(&outputs, &inputs, args.force)?;
    let report = master_table::validate_table(BufReader::new(std::fs::File::open(&args.master)?))?;
    let mut errors = report.errors;
    if args.schema.is_some() {
        let known: BTreeSet<_> = schema.sources.iter().map(|(s, _)| s.as_str()).collect();
        for (i, line) in BufReader::new(std::fs::File::open(&args.master)?)
            .split(b'\n')
            .enumerate()
            .skip(1)
        {
            let line = line?;
            if let Ok(line) = std::str::from_utf8(&line)
                && let Ok(row) = Row::parse_line(line.trim_end_matches('\r'), i + 1)
                && !known.contains(row.get("source"))
            {
                errors.push(format!(
                    "元データの {} 行目: unknown source: {}",
                    i + 1,
                    row.get("source")
                ));
            }
        }
    }
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    let mut rows = master_table::parse_master(&std::fs::read_to_string(&args.master)?)?;
    rows.retain(|r| args.tier == "wide" || r.get("tier") == "curated");
    rows.sort_by(|a, b| {
        (a.get("key"), role(a), a.get("move")).cmp(&(b.get("key"), role(b), b.get("move")))
    });
    let (read_groups, _) = reads::load(&logs, args.min_nodes, args.draw_pin)?;
    let edges = graph::continuations(&rows)?;
    let active = rows
        .iter()
        .filter(|r| r.get("role") != "record")
        .map(|r| (r.get("key").into(), r.get("move").into()))
        .collect();
    let depths = graph::depths(&edges, &active);
    let mut groups = BTreeMap::<&str, Vec<&Row>>::new();
    for row in &rows {
        groups.entry(row.get("key")).or_default().push(row);
    }
    let mut db = String::from("#YANEURAOU-DB2016 1.00\n");
    for (key, entries) in &groups {
        db.push_str(&format!("sfen {key} 0\n"));
        for row in entries {
            let (value, count) = match row.get("role") {
                "main" => (100, 2),
                "alt" => (100, 1),
                _ => (-30000, 0),
            };
            db.push_str(&format!("{} none {value} 0 {count}\n", row.get("move")));
        }
    }
    let digest = format!("{:x}", Sha256::digest(db.as_bytes()));
    let filename = outputs[0]
        .file_name()
        .and_then(|s| s.to_str())
        .context("book filename must be UTF-8")?;
    let mut presets = ["default", "random"]
        .map(|name| format!("# preset: {name}\n# book: {filename} sha256 {digest}\n"));
    let mut positions = Vec::new();
    let mut preset_count = 0;
    for (key, entries) in &groups {
        let selectable: Vec<_> = entries
            .iter()
            .filter(|r| r.get("role") != "record")
            .map(|r| r.get("move"))
            .collect();
        if selectable.len() >= 2 {
            preset_count += 1;
            presets[0].push_str(&format!("sfen {key} {}\n", selectable[0]));
            presets[1].push_str(&format!("sfen {key} {}\n", selectable.join(" ")));
        }
        let ply = entries
            .iter()
            .filter(|r| !r.get("ply").is_empty())
            .map(|r| r.get("ply").parse::<num_bigint::BigInt>())
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .min();
        let mut moves = Vec::new();
        for row in entries {
            let pair = (key.to_string(), row.get("move").to_string());
            let mut data: Vec<_> = ["role", "source", "nodes", "note", "since", "tier"]
                .into_iter()
                .map(|f| (f, Json::string(row.get(f))))
                .collect();
            data.push((
                "lineage",
                Json::Array(if row.get("lineage").is_empty() {
                    vec![]
                } else {
                    row.get("lineage").split(',').map(Json::string).collect()
                }),
            ));
            let values = evals(row)?;
            data.push((
                "evals",
                Json::Array(
                    values.into_iter().map(|v| Json::Number(numbers::eval_number(v))).collect(),
                ),
            ));
            data.push(("replies", Json::Array(edges[&pair].keys().map(Json::string).collect())));
            data.push(("depth_after", Json::integer(depths[&pair])));
            moves.push((row.get("move"), Json::object(data)));
        }
        let mut fields = vec![
            ("ply", ply.map_or(Json::Null, Json::integer)),
            ("moves", Json::object(moves)),
        ];
        if let Some(reads) = read_groups.get(*key) {
            let kept = reads::deep(reads);
            if !kept.is_empty() {
                let main = entries.iter().find(|r| r.get("role") == "main").map(|r| r.get("move"));
                let values: Vec<_> = kept
                    .iter()
                    .filter_map(|r| {
                        r.scores
                            .iter()
                            .find(|(m, _)| Some(m.as_str()) == main)
                            .map(|(_, v)| v.clone())
                    })
                    .collect();
                let tops: Vec<_> = kept.iter().map(|r| r.scores[0].1.clone()).collect();
                fields.push((
                    "read",
                    Json::object([
                        ("n", Json::integer(kept.len())),
                        (
                            "nodes",
                            Json::string(numbers::rounded_nodes(
                                kept.iter().map(|r| &r.nodes).max().unwrap(),
                            )?),
                        ),
                        ("top", Json::Number(numbers::repr(numbers::mean(&tops)?))),
                        (
                            "main",
                            if values.is_empty() {
                                Json::Null
                            } else {
                                Json::Number(numbers::repr(numbers::mean(&values)?))
                            },
                        ),
                        ("main_n", Json::integer(values.len())),
                    ]),
                ));
            }
        }
        positions.push((*key, Json::object(fields)));
        if entries.iter().any(|r| r.get("role") == "main" && r.get("tier") == "curated")
            && entries.iter().any(|r| r.get("role") == "alt" && r.get("tier") == "wide")
        {
            writeln!(
                out,
                "WARNING: curated main with wide alt: random preset selects multiple moves: {key}"
            )?;
        }
    }
    let annotation = Json::object([
        ("format", Json::string("rshogi-book-annotations")),
        ("version", Json::integer(1)),
        (
            "book",
            Json::object([
                ("file", Json::string(filename)),
                ("sha256", Json::string(&digest)),
            ]),
        ),
        (
            "sources",
            Json::object(schema.sources.iter().map(|(k, v)| (k, Json::string(v)))),
        ),
        ("positions", Json::object(positions)),
    ])
    .render();
    let bad = check::export_bad_progressions(annotation.as_bytes(), args.bad_eval, out)?;
    out.flush()?;
    ensure!(
        !args.strict_bad_eval || bad == 0,
        "strict-bad-eval: bad positions reachable; no files written"
    );
    if let Some(base) = &args.base {
        let previous = book_diff::read(base, true)?;
        let added = groups.keys().filter(|k| !previous.contains_key(**k)).count();
        let removed: Vec<_> =
            previous.keys().filter(|k| !groups.contains_key(k.as_str())).collect();
        let changed = groups
            .iter()
            .filter(|(k, entries)| {
                previous.get(**k).is_some_and(|p| {
                    p.moves.iter().map(String::as_str).collect::<BTreeSet<_>>()
                        != entries.iter().map(|r| r.get("move")).collect()
                })
            })
            .count();
        writeln!(
            out,
            "base diff: added positions={added}, removed positions={}, changed move positions={changed}",
            removed.len()
        )?;
        for key in removed.into_iter().take(10) {
            writeln!(out, "  removed: {key}")?;
        }
    }
    let [default, random] = presets;
    files::write(
        &outputs,
        &inputs,
        args.force,
        &[
            db.into_bytes(),
            default.into_bytes(),
            random.into_bytes(),
            annotation.into_bytes(),
        ],
    )?;
    print_counts(&rows, &schema, out)?;
    writeln!(
        out,
        "preset positions (each): {preset_count}; reply links: {}; max depth_after: {}",
        edges.values().map(BTreeMap::len).sum::<usize>(),
        depths.values().max().unwrap_or(&0)
    )?;
    writeln!(out, "book: {filename}; sha256: {digest}")?;
    out.flush()?;
    Ok(())
}
fn role(row: &Row) -> usize {
    ROLES.iter().position(|&s| s == row.get("role")).unwrap()
}
fn evals(row: &Row) -> Result<Vec<f64>> {
    if row.get("evals").is_empty() {
        return Ok(vec![]);
    }
    Ok(row
        .get("evals")
        .split(',')
        .map(|s| s.trim().parse())
        .collect::<std::result::Result<_, _>>()?)
}
fn print_counts(rows: &[Row], schema: &Schema, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "positions: {}; moves: {}",
        rows.iter().map(|r| r.get("key")).collect::<BTreeSet<_>>().len(),
        rows.len()
    )?;
    for tier in ["curated", "wide"] {
        let selected: Vec<_> = rows.iter().filter(|r| r.get("tier") == tier).collect();
        writeln!(
            out,
            "tier {tier}: positions={}, moves={}",
            selected.iter().map(|r| r.get("key")).collect::<BTreeSet<_>>().len(),
            selected.len()
        )?;
    }
    for field in ["role", "source", "since"] {
        let mut counts = BTreeMap::<&str, usize>::new();
        for row in rows {
            *counts.entry(row.get(field)).or_default() += 1;
        }
        let values: Vec<_> = match field {
            "role" => ROLES.to_vec(),
            "source" if !schema.sources.is_empty() => {
                schema.sources.iter().map(|(k, _)| k.as_str()).collect()
            }
            _ => counts.keys().copied().collect(),
        };
        writeln!(
            out,
            "{field}: {}",
            values
                .iter()
                .map(|v| format!("{v}={}", counts.get(v).unwrap_or(&0)))
                .collect::<Vec<_>>()
                .join(", ")
        )?;
    }
    Ok(())
}
